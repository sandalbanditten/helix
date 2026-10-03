use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use helix_term::application::Application;
use helix_view::doc;

use super::*;

/// How long nothing should happen for when a change is not to be followed: well past the
/// watcher's batching and a background read.
const QUIET: Duration = Duration::from_millis(400);

fn session(path: &Path) -> anyhow::Result<Session> {
    let mut config = test_config();
    config.editor.auto_reload = true;
    let app = AppBuilder::new()
        .with_file(path, None)
        .with_config(config)
        .build()?;
    Ok(Session::new(app))
}

/// A file holding `text` in a directory of its own.
fn file(text: &str) -> anyhow::Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir()?;
    let path = helix_stdx::path::canonicalize(dir.path().join("file.txt"));
    fs::write(&path, text)?;
    Ok((dir, path))
}

fn text(app: &Application) -> String {
    doc!(app.editor).text().to_string()
}

fn status(app: &Application) -> String {
    app.editor
        .get_status()
        .map(|(status, _)| status.to_string())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn buffers_follow_their_files() -> anyhow::Result<()> {
    let (_dir, path) = file("one\n")?;
    let mut session = session(&path)?;
    session.keys("").await?;

    fs::write(&path, "two\n")?;
    session
        .until("the reload", |app| text(app) == "two\n")
        .await;
    assert!(!doc!(session.app.editor).is_modified());
    assert!(status(&session.app).ends_with("file.txt reloaded"));

    // The reload is a step of the history.
    session.keys("u").await?;
    assert_eq!(text(&session.app), "one\n");
    assert!(doc!(session.app.editor).is_modified());
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn own_writes_are_not_reloaded() -> anyhow::Result<()> {
    let (_dir, path) = file("one\n")?;
    let mut session = session(&path)?;
    session.keys("ihello <esc>:w<ret>").await?;
    // Typing while the watcher still reports the write.
    session.keys("ihi <esc>").await?;
    session.wait(QUIET).await;
    assert_eq!(text(&session.app), "hello hi one\n");
    assert!(doc!(session.app.editor).is_modified());
    assert_eq!(fs::read_to_string(&path)?, "hello one\n");
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn rewrites_of_the_same_text_change_nothing() -> anyhow::Result<()> {
    let (_dir, path) = file("one\n")?;
    let mut session = session(&path)?;
    session.keys("").await?;
    fs::write(&path, "one\n")?;
    session.wait(QUIET).await;
    assert!(!status(&session.app).contains("reloaded"));
    // The new time of the file is taken as known, so writing needs no `!`.
    session.keys("ihello <esc>:w<ret>").await?;
    assert_eq!(fs::read_to_string(&path)?, "hello one\n");
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn deleted_files_are_told_of_and_followed_when_back() -> anyhow::Result<()> {
    let (_dir, path) = file("one\n")?;
    let mut session = session(&path)?;
    session.keys("").await?;

    fs::remove_file(&path)?;
    session
        .until("the deletion", |app| {
            status(app).ends_with("file.txt was deleted on disk")
        })
        .await;
    assert_eq!(text(&session.app), "one\n");

    fs::write(&path, "back\n")?;
    session
        .until("the reload", |app| text(app) == "back\n")
        .await;
    session.quit().await
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn symlinks_follow_the_files_they_link_to() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("elsewhere/file.txt");
    fs::create_dir(target.parent().unwrap())?;
    fs::write(&target, "one\n")?;
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink(&target, &link)?;
    let mut session = session(&link)?;
    session.keys("").await?;

    fs::write(&target, "two\n")?;
    session
        .until("the reload", |app| text(app) == "two\n")
        .await;
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn buffers_opened_or_written_elsewhere_are_followed() -> anyhow::Result<()> {
    let (dir, path) = file("one\n")?;
    let other = dir.path().join("other.txt");
    fs::write(&other, "other\n")?;
    let mut session = session(&path)?;
    session
        .keys(&format!(":o {}<ret>", other.display()))
        .await?;

    fs::write(&other, "changed\n")?;
    session
        .until("the reload of the opened file", |app| {
            text(app) == "changed\n"
        })
        .await;

    let moved = dir.path().join("sub/moved.txt");
    session
        .keys(&format!(":w! {}<ret>", moved.display()))
        .await?;
    fs::write(&moved, "moved\n")?;
    session
        .until("the reload of the file written elsewhere", |app| {
            text(app) == "moved\n"
        })
        .await;
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn all_views_of_a_reloaded_buffer_follow() -> anyhow::Result<()> {
    let (_dir, path) = file("one\ntwo\nthree\n")?;
    let mut session = session(&path)?;
    // A jump to the end in the second view, which the shorter text no longer has.
    session.keys(":vsplit<ret>ge").await?;
    session.keys("<C-w>w").await?;

    fs::write(&path, "one\n")?;
    session
        .until("the reload", |app| text(app) == "one\n")
        .await;
    // Editing and jumping back there maps the jumps through the reload.
    session.keys("<C-w>wix<esc><C-o>").await?;
    assert!(text(&session.app).contains('x'));
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_followed_with_auto_reload_off() -> anyhow::Result<()> {
    let (_dir, path) = file("one\n")?;
    let app = AppBuilder::new().with_file(&path, None).build()?;
    let mut session = Session::new(app);
    session.keys("").await?;
    fs::write(&path, "two\n")?;
    session.wait(QUIET).await;
    assert_eq!(text(&session.app), "one\n");
    session.quit().await
}

/// The screen as last drawn, its rows without trailing spaces.
fn screen(app: &Application) -> String {
    let screen = app.screen();
    let rows: Vec<String> = screen
        .content
        .chunks(screen.area.width as usize)
        .map(|row| {
            let row: String = row.iter().map(|cell| cell.symbol.as_str()).collect();
            row.trim_end().to_owned()
        })
        .collect();
    rows.join("\n")
}

/// Whether the question about a buffer with unsaved changes is up.
fn asks(app: &Application) -> bool {
    screen(app).contains("changed on disk")
}

/// A buffer of a file holding "one\n", with "hello " typed into it, and the file changed to
/// "two\n" since.
async fn conflict() -> anyhow::Result<(tempfile::TempDir, PathBuf, Session)> {
    let (dir, path) = file("one\n")?;
    let mut session = session(&path)?;
    session.keys("ihello <esc>").await?;
    fs::write(&path, "two\n")?;
    session.until("the question", asks).await;
    Ok((dir, path, session))
}

#[tokio::test(flavor = "multi_thread")]
async fn unsaved_changes_are_asked_about_before_reloading() -> anyhow::Result<()> {
    let (_dir, _path, mut session) = conflict().await?;
    let screen = screen(&session.app);
    assert!(screen.contains("r  Reload"), "{screen}");
    assert!(screen.contains("k  Keep unsaved changes"), "{screen}");
    assert!(!screen.contains("Reload all"), "{screen}");
    assert_eq!(text(&session.app), "hello one\n");

    session.keys("r").await?;
    assert!(!asks(&session.app));
    assert_eq!(text(&session.app), "two\n");
    assert!(!doc!(session.app.editor).is_modified());
    assert!(status(&session.app).ends_with("file.txt reloaded"));
    // The unsaved changes are a step back.
    session.keys("u").await?;
    assert_eq!(text(&session.app), "hello one\n");
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn kept_changes_overwrite_the_file_on_a_plain_write() -> anyhow::Result<()> {
    let (_dir, path, mut session) = conflict().await?;
    session.keys("k").await?;
    assert!(!asks(&session.app));
    assert!(status(&session.app).ends_with("file.txt kept"));
    assert_eq!(text(&session.app), "hello one\n");
    assert!(doc!(session.app.editor).is_modified());

    session.keys(":w<ret>").await?;
    assert_eq!(fs::read_to_string(&path)?, "hello one\n");
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn stray_keys_are_swallowed_and_escape_keeps() -> anyhow::Result<()> {
    let (_dir, _path, mut session) = conflict().await?;
    session.keys("dd").await?;
    assert!(asks(&session.app));
    assert_eq!(text(&session.app), "hello one\n");
    session.keys("<esc>").await?;
    assert!(!asks(&session.app));
    assert!(status(&session.app).ends_with("file.txt kept"));
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_question_waits_for_insert_mode_to_end() -> anyhow::Result<()> {
    let (_dir, path) = file("one\n")?;
    let mut session = session(&path)?;
    session.keys("ihello ").await?;
    fs::write(&path, "two\n")?;
    session.wait(QUIET).await;
    assert!(!asks(&session.app));
    // Typing goes on into the buffer.
    session.keys("kr").await?;
    assert_eq!(text(&session.app), "hello krone\n");
    session.keys("<esc>").await?;
    assert!(asks(&session.app));
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_open_prompt_keeps_its_keys() -> anyhow::Result<()> {
    let (_dir, path) = file("one\n")?;
    let mut session = session(&path)?;
    session.keys("ihello <esc>:").await?;
    fs::write(&path, "two\n")?;
    session.wait(QUIET).await;
    // Escape closes the prompt rather than answering.
    session.keys("<esc>").await?;
    assert!(asks(&session.app));
    assert!(!status(&session.app).contains("kept"));
    session.keys("k").await?;
    assert!(!asks(&session.app));
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn many_buffers_are_answered_at_once() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let paths: Vec<PathBuf> = ["a.txt", "b.txt", "c.txt"]
        .iter()
        .map(|name| helix_stdx::path::canonicalize(dir.path().join(name)))
        .collect();
    let mut config = test_config();
    config.editor.auto_reload = true;
    let mut builder = AppBuilder::new().with_config(config);
    for path in &paths {
        fs::write(path, "one\n")?;
        builder = builder.with_file(path, None);
    }
    let mut session = Session::new(builder.build()?);
    session
        .keys("ix<esc>:bn<ret>ix<esc>:bn<ret>ix<esc>")
        .await?;
    for path in &paths {
        fs::write(path, "two\n")?;
    }
    session
        .until("all three asked about", |app| {
            screen(app).contains("R  Reload all 3")
        })
        .await;
    assert!(screen(&session.app).contains("K  Keep all 3"));

    // One answered, then the other two at once.
    session.keys("k").await?;
    assert!(screen(&session.app).contains("R  Reload all 2"));
    session.keys("R").await?;
    assert!(!asks(&session.app));
    assert_eq!(status(&session.app), "2 buffers reloaded");
    let texts: Vec<String> = session
        .app
        .editor
        .documents()
        .map(|doc| doc.text().to_string())
        .collect();
    assert_eq!(texts.iter().filter(|text| *text == "two\n").count(), 2);
    assert_eq!(texts.iter().filter(|text| *text == "xone\n").count(), 1);
    session.quit().await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_known_text_back_on_disk_asks_nothing() -> anyhow::Result<()> {
    let (_dir, path, mut session) = conflict().await?;
    // Like `git stash` followed by `git stash pop`.
    fs::write(&path, "one\n")?;
    session.until("the question to go", |app| !asks(app)).await;
    assert_eq!(text(&session.app), "hello one\n");
    session.keys(":w<ret>").await?;
    assert_eq!(fs::read_to_string(&path)?, "hello one\n");
    session.quit().await
}

/// The diff base of the buffer and its number of hunks.
fn diff(app: &Application) -> Option<(String, u32)> {
    let handle = doc!(app.editor).diff_handle()?;
    let diff = handle.load();
    Some((diff.diff_base().to_string(), diff.len()))
}

fn head(app: &Application) -> Option<String> {
    Some(doc!(app.editor).version_control_head()?.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn moving_head_refreshes_the_diff_and_the_branch() -> anyhow::Result<()> {
    let (dir, path) = file("one\n")?;
    git(dir.path(), &["init"]);
    git(dir.path(), &["add", "file.txt"]);
    git(dir.path(), &["commit", "-m", "one"]);
    let mut session = session(&path)?;
    session.keys("").await?;
    assert_eq!(head(&session.app).as_deref(), Some("main"));

    fs::write(&path, "two\n")?;
    session
        .until("the reload against HEAD", |app| {
            diff(app) == Some(("one\n".into(), 1))
        })
        .await;

    // A commit moves HEAD without touching the file.
    git(dir.path(), &["commit", "-am", "two"]);
    session
        .until("the diff against the new HEAD", |app| {
            diff(app) == Some(("two\n".into(), 0))
        })
        .await;

    git(dir.path(), &["switch", "-c", "topic/new"]);
    session
        .until("the new branch", |app| {
            head(app).as_deref() == Some("topic/new")
        })
        .await;
    // The ref of the new branch, in a directory that didn't exist, is followed too.
    fs::write(&path, "three\n")?;
    session
        .until("the reload", |app| text(app) == "three\n")
        .await;
    git(dir.path(), &["commit", "-am", "three"]);
    session
        .until("the diff against the commit on the new branch", |app| {
            diff(app) == Some(("three\n".into(), 0))
        })
        .await;
    session.quit().await
}

/// Measurements of how fast changes are followed and what following costs. Run them with
/// `cargo test --release --features integration -p helix-term --test integration --
/// auto_reload::measure --ignored --nocapture --test-threads 1`.
mod measure {
    use std::{
        sync::{Arc, Mutex},
        time::Instant,
    };

    use super::*;

    /// Records when the editor's main loop runs callbacks, asked for every millisecond, to find
    /// its longest stall.
    struct Probe {
        times: Arc<Mutex<Vec<Instant>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Probe {
        fn start() -> Self {
            let times = Arc::new(Mutex::new(Vec::new()));
            let times_ = times.clone();
            let task = tokio::spawn(async move {
                loop {
                    let times = times_.clone();
                    helix_term::job::dispatch(move |_, _| {
                        times.lock().unwrap().push(Instant::now())
                    })
                    .await;
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            });
            Self { times, task }
        }

        /// The longest time between two callbacks.
        fn longest_stall(self) -> Duration {
            self.task.abort();
            let times = self.times.lock().unwrap();
            times
                .windows(2)
                .map(|pair| pair[1] - pair[0])
                .max()
                .unwrap_or_default()
        }
    }

    /// A text of about `bytes` bytes, in lines.
    fn text_of(bytes: usize) -> String {
        let mut text = String::with_capacity(bytes + 64);
        let mut line = 0;
        while text.len() < bytes {
            text.push_str(&format!(
                "line {line} of some text, long enough to be a line\n"
            ));
            line += 1;
        }
        text
    }

    /// The process' CPU time so far, from `/proc/self/stat`.
    #[cfg(target_os = "linux")]
    fn cpu_time() -> Duration {
        let stat = fs::read_to_string("/proc/self/stat").unwrap();
        let fields: Vec<&str> = stat
            .rsplit(')')
            .next()
            .unwrap()
            .split_whitespace()
            .collect();
        // utime and stime, the 14th and 15th fields, in clock ticks of 10 ms.
        let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
        Duration::from_millis(ticks * 10)
    }

    fn version(app: &Application) -> i32 {
        doc!(app.editor).version()
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement, not a check"]
    async fn measure_reloads() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let mut paths = Vec::new();
        for (name, bytes) in [("10KB", 10 << 10), ("1MB", 1 << 20), ("50MB", 50 << 20)] {
            let path = dir.path().join(format!("{name}.txt"));
            fs::write(&path, text_of(bytes))?;
            paths.push((name, path));
        }
        let mut session = session(&paths[0].1)?;
        session.keys("").await?;

        let probe = Probe::start();
        session.wait(Duration::from_millis(300)).await;
        println!("idle: longest stall {:?}", probe.longest_stall());

        for (name, path) in &paths {
            session.keys(&format!(":o {}<ret>", path.display())).await?;
            session.wait(Duration::from_millis(300)).await;
            let before = version(&session.app);
            // One line changed in the middle.
            let mut text = fs::read_to_string(path)?;
            let middle = text.len() / 2;
            let start = text[..middle].rfind('\n').unwrap() + 1;
            text.insert_str(start, "changed ");
            let probe = Probe::start();
            let written = Instant::now();
            fs::write(path, &text)?;
            session
                .until("the reload", |app| version(app) != before)
                .await;
            let latency = written.elapsed();
            session.wait(Duration::from_millis(100)).await;
            println!(
                "{name}: reloaded after {latency:?}, longest stall {:?}",
                probe.longest_stall()
            );
        }
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement, not a check"]
    async fn measure_a_burst() -> anyhow::Result<()> {
        const FILES: usize = 200;
        let dir = tempfile::tempdir()?;
        let paths: Vec<PathBuf> = (0..FILES)
            .map(|i| helix_stdx::path::canonicalize(dir.path().join(format!("file-{i:03}.txt"))))
            .collect();
        for path in &paths {
            fs::write(path, text_of(10 << 10))?;
        }
        let mut config = test_config();
        config.editor.auto_reload = true;
        let mut builder = AppBuilder::new().with_config(config);
        for path in &paths {
            builder = builder.with_file(path, None);
        }
        let mut session = Session::new(builder.build()?);
        session.keys("").await?;

        let probe = Probe::start();
        let written = Instant::now();
        for path in &paths {
            fs::write(path, format!("changed\n{}", text_of(10 << 10)))?;
        }
        let rewritten = written.elapsed();
        session
            .until("the reloads", |app| {
                app.editor
                    .documents()
                    .all(|doc| doc.text().line(0) == "changed\n")
            })
            .await;
        println!(
            "{FILES} files rewritten in {rewritten:?}, all reloaded after {:?}, longest stall {:?}, status {:?}",
            written.elapsed(),
            probe.longest_stall(),
            status(&session.app)
        );
        session.quit().await
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement, not a check"]
    async fn measure_a_busy_neighbour() -> anyhow::Result<()> {
        const PERIOD: Duration = Duration::from_secs(2);
        let (dir, path) = file("one\n")?;
        let mut session = session(&path)?;
        session.keys("").await?;

        let cpu = cpu_time();
        session.wait(PERIOD).await;
        let idle = cpu_time() - cpu;

        let log = dir.path().join("log.txt");
        let writer = std::thread::spawn(move || {
            use std::io::Write;
            let mut file = fs::File::create(log).unwrap();
            let start = Instant::now();
            let mut writes = 0;
            while start.elapsed() < PERIOD {
                writeln!(file, "a line of the log").unwrap();
                writes += 1;
                std::thread::sleep(Duration::from_millis(1));
            }
            writes
        });
        let cpu = cpu_time();
        session.wait(PERIOD).await;
        let writes = writer.join().unwrap();
        let busy = cpu_time() - cpu;
        println!(
            "CPU over {PERIOD:?}: {idle:?} idle, {busy:?} beside {writes} writes to another file"
        );
        assert!(!status(&session.app).contains("reloaded"));
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement, not a check"]
    async fn measure_a_slow_write() -> anyhow::Result<()> {
        const CHUNKS: usize = 10;
        let (_dir, path) = file("one\n")?;
        let mut session = session(&path)?;
        session.keys("").await?;

        let chunk = text_of(1 << 20);
        let writer = {
            let (path, chunk) = (path.clone(), chunk.clone());
            std::thread::spawn(move || {
                use std::io::Write;
                let mut file = fs::File::create(path).unwrap();
                for _ in 0..CHUNKS {
                    file.write_all(chunk.as_bytes()).unwrap();
                    std::thread::sleep(Duration::from_millis(50));
                }
            })
        };
        let start = Instant::now();
        let mut versions = vec![version(&session.app)];
        let whole = chunk.len() * CHUNKS;
        while doc!(session.app.editor).text().len_bytes() != whole {
            assert!(start.elapsed() < Duration::from_secs(20), "timed out");
            session.run_for(Duration::from_millis(10)).await;
            let version = version(&session.app);
            if versions.last() != Some(&version) {
                versions.push(version);
            }
        }
        writer.join().unwrap();
        println!(
            "{CHUNKS} MB written in {CHUNKS} chunks 50 ms apart: {} reloads, the whole file after {:?}",
            versions.len() - 1,
            start.elapsed()
        );
        session.quit().await
    }

    /// Opens 500 files in 50 directories of a repository, then moves HEAD.
    async fn open_a_repository(auto_reload: bool) -> anyhow::Result<()> {
        const DIRS: usize = 50;
        const FILES: usize = 10;
        let dir = tempfile::tempdir()?;
        git(dir.path(), &["init"]);
        let mut config = test_config();
        config.editor.auto_reload = auto_reload;
        let mut builder = AppBuilder::new().with_config(config);
        for d in 0..DIRS {
            let sub = dir.path().join(format!("dir-{d:02}"));
            fs::create_dir(&sub)?;
            for f in 0..FILES {
                let path = sub.join(format!("file-{f}.txt"));
                fs::write(&path, "one\n")?;
                builder = builder.with_file(path, None);
            }
        }
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-m", "one"]);
        let start = Instant::now();
        let mut session = Session::new(builder.build()?);
        session.keys("").await?;
        println!(
            "opening {} files in {DIRS} directories of a repository with auto-reload {auto_reload}: {:?}",
            DIRS * FILES,
            start.elapsed()
        );
        if auto_reload {
            let probe = Probe::start();
            git(dir.path(), &["commit", "--allow-empty", "-m", "two"]);
            session.wait(Duration::from_millis(500)).await;
            println!("a commit: longest stall {:?}", probe.longest_stall());
        }
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement, not a check"]
    async fn measure_a_repository_with_auto_reload() -> anyhow::Result<()> {
        open_a_repository(true).await
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement, not a check"]
    async fn measure_a_repository_without_auto_reload() -> anyhow::Result<()> {
        open_a_repository(false).await
    }

    async fn open_many(auto_reload: bool) -> anyhow::Result<()> {
        const FILES: usize = 500;
        let dir = tempfile::tempdir()?;
        let mut config = test_config();
        config.editor.auto_reload = auto_reload;
        let mut builder = AppBuilder::new().with_config(config);
        for i in 0..FILES {
            let path = dir.path().join(format!("file-{i:03}.txt"));
            fs::write(&path, "one\n")?;
            builder = builder.with_file(path, None);
        }
        let start = Instant::now();
        let mut session = Session::new(builder.build()?);
        session.keys("").await?;
        println!(
            "opening {FILES} files with auto-reload {auto_reload}: {:?}",
            start.elapsed()
        );

        #[cfg(not(windows))]
        if auto_reload {
            let probe = Probe::start();
            let start = Instant::now();
            session.send_event(termina::event::Event::FocusIn)?;
            session.wait(Duration::from_millis(300)).await;
            println!(
                "focus gained with {FILES} buffers: longest stall {:?} within {:?}",
                probe.longest_stall(),
                start.elapsed()
            );
        }
        session.quit().await
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement, not a check"]
    async fn measure_opening_with_auto_reload() -> anyhow::Result<()> {
        open_many(true).await
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement, not a check"]
    async fn measure_opening_without_auto_reload() -> anyhow::Result<()> {
        open_many(false).await
    }
}
