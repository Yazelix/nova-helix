use super::*;
use helix_core::{
    completion::{CompletionItem, CompletionProvider},
    Transaction,
};
use helix_loader::workspace_trust::WorkspaceTrust;
use helix_view::{document::Mode, input::parse_macro};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::PathBuf,
    time::Duration,
};
use tempfile::TempDir;

// Steel has one global engine; concurrent Applications race its safepoint flag.
#[cfg(feature = "steel")]
static APPLICATION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn private_dir() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn application(listen: PathBuf) -> Application {
    let mut config = Config::default();
    config.editor.lsp.enable = false;
    config.editor.word_completion.enable = false;
    Application::new(
        Args {
            listen: Some(listen),
            ..Args::default()
        },
        config,
        syntax::Loader::new(
            helix_loader::config::default_lang_config()
                .try_into()
                .unwrap(),
        )
        .unwrap(),
        WorkspaceTrust::fully_trusted(),
    )
    .unwrap()
}

async fn keys(app: &mut Application, input: &str) {
    for key in parse_macro(input).unwrap() {
        app.handle_terminal_events(Ok(termina::Event::Key(termina::event::KeyEvent::from(key))))
            .await;
    }
}

async fn request(app: &mut Application, socket: &Path, paths: Vec<String>) -> serde_json::Value {
    let socket = socket.to_owned();
    let mut client = tokio::task::spawn_blocking(move || {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        writeln!(
            stream,
            "{}",
            json!({"version":1,"id":"app","action":"open","paths":paths})
        )
        .unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        serde_json::from_str::<serde_json::Value>(&response).unwrap()
    });
    let result = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            tokio::select! {
                result = &mut client => break result.unwrap(),
                callback = app.jobs.callbacks.recv() => {
                    app.jobs.handle_callback(&mut app.editor, &mut app.compositor, Ok(callback));
                }
            }
        }
    })
    .await
    .unwrap();
    while let Ok(callback) = app.jobs.callbacks.try_recv() {
        app.jobs
            .handle_callback(&mut app.editor, &mut app.compositor, Ok(Some(callback)));
    }
    app.render().await;
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_open_preserves_buffers_and_cancels_old_document_input() {
    #[cfg(feature = "steel")]
    let _application = APPLICATION_LOCK.lock().await;
    let dir = private_dir();
    let socket = dir.path().join("editor.sock");
    let first = dir.path().join("a.txt");
    let second = dir.path().join("b.txt");
    fs::write(&first, "first\n").unwrap();
    fs::write(&second, "one\ntwo\nthree\n").unwrap();
    let mut app = application(socket.clone());
    assert!(app.remote.is_some());
    let scratch = doc!(app.editor).id();
    assert_eq!(
        request(&mut app, &socket, vec![first.display().to_string()]).await["status"],
        "ok"
    );
    assert!(
        app.editor.document(scratch).is_none(),
        "unmodified scratch survived replacement"
    );
    let first_id = doc!(app.editor).id();
    keys(&mut app, "iedit").await;
    let size = app.compositor.size();
    let completion_transaction = Transaction::new(doc!(app.editor).text());
    app.compositor
        .find::<ui::EditorView>()
        .unwrap()
        .set_completion(
            &mut app.editor,
            vec![crate::handlers::completion::CompletionItem::Other(
                CompletionItem {
                    transaction: completion_transaction,
                    label: "edited".into(),
                    kind: "word".into(),
                    documentation: None,
                    provider: CompletionProvider::Word,
                },
            )],
            0,
            size,
        );
    assert!(app
        .compositor
        .find::<ui::EditorView>()
        .unwrap()
        .completion
        .is_some());
    let signature = ui::lsp::signature_help::SignatureHelp::new(
        "text".into(),
        app.editor.syn_loader.clone(),
        0,
        None,
        vec![ui::lsp::signature_help::Signature {
            signature: "f(arg)".into(),
            signature_doc: None,
            active_param_range: None,
        }],
    );
    app.compositor.push(Box::new(ui::Popup::new(
        ui::lsp::signature_help::SignatureHelp::ID,
        signature,
    )));
    assert_eq!(
        request(&mut app, &socket, vec![format!("{}:2:3", second.display())]).await["status"],
        "ok"
    );
    assert_eq!(app.editor.mode(), Mode::Normal);
    assert!(app
        .compositor
        .find::<ui::EditorView>()
        .unwrap()
        .completion
        .is_none());
    assert!(ui::lsp::signature_help::SignatureHelp::visible_popup(&mut app.compositor).is_none());
    let old = app.editor.document(first_id).unwrap();
    assert!(old.is_modified());
    assert_eq!(old.text().to_string(), "editfirst\n");
    let (view, doc) = current!(app.editor);
    let second_id = doc.id();
    let selection = doc.selection(view.id).clone();
    assert_eq!(selection.primary().cursor(doc.text().slice(..)), 6);
    fs::write(&second, "changed on disk\n").unwrap();
    assert_eq!(
        request(&mut app, &socket, vec![second.display().to_string()]).await["status"],
        "ok"
    );
    let (view, doc) = current!(app.editor);
    assert_eq!(doc.id(), second_id);
    assert_eq!(doc.selection(view.id), &selection);
    assert_eq!(doc.text().to_string(), "one\ntwo\nthree\n");

    keys(&mut app, ":buffer").await;
    assert_eq!(
        app.compositor.find::<ui::Prompt>().unwrap().line(),
        "buffer"
    );
    let missing = dir.path().join("new file.txt");
    assert_eq!(
        request(&mut app, &socket, vec![missing.display().to_string()]).await["status"],
        "ok"
    );
    assert_eq!(doc!(app.editor).path(), Some(missing.as_path()));
    assert!(!missing.exists());
    assert!(app.compositor.find::<ui::Prompt>().is_none());
    app.compositor
        .push(Box::new(ui::overlay::overlaid(ui::file_picker(
            &app.editor,
            dir.path().to_owned(),
        ))));
    keys(&mut app, "b.txt").await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            app.render().await;
            let selection = app
                .compositor
                .find::<ui::overlay::Overlay<ui::Picker<PathBuf, ui::FilePickerData>>>()
                .and_then(|picker| picker.content.selection());
            if selection == Some(&second) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        request(&mut app, &socket, vec![first.display().to_string()]).await["status"],
        "ok"
    );
    assert_eq!(
        app.compositor
            .find::<ui::overlay::Overlay<ui::Picker<PathBuf, ui::FilePickerData>>>()
            .unwrap()
            .content
            .selection(),
        Some(&second)
    );
    assert_eq!(doc!(app.editor).id(), first_id);
    keys(&mut app, "<esc>u").await;
    assert_eq!(
        doc!(app.editor).text().to_string(),
        "first\n",
        "remote switch lost insert undo history"
    );
    assert!(app.close().await.is_empty());
    assert!(!socket.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_bind_failure_keeps_editor_live_and_open_errors_name_the_path() {
    #[cfg(feature = "steel")]
    let _application = APPLICATION_LOCK.lock().await;
    let dir = private_dir();
    let path = dir.path().join("occupied");
    fs::write(&path, "keep").unwrap();
    let mut app = application(path.clone());
    assert!(app.remote.is_none());
    assert!(app.editor.get_status().unwrap().0.contains("--listen"));
    keys(&mut app, "ikept<esc>").await;
    assert_eq!(doc!(app.editor).text().to_string(), "kept\n");
    assert_eq!(fs::read_to_string(&path).unwrap(), "keep");
    assert!(app.close().await.is_empty());

    // A separate runtime is not needed: this app reuses the same job receiver.
    // Open failure is tested on the first app by attaching its server explicitly.
    app.remote = Some(crate::remote::OpenServer::start(&dir.path().join("open.sock")).unwrap());
    let file = dir.path().join("file.txt");
    fs::write(&file, "text\n").unwrap();
    let child = dir.path().join("unreadable.txt");
    fs::write(&child, "private\n").unwrap();
    fs::set_permissions(&child, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(fs::read(&child).is_err());
    let result = request(
        &mut app,
        &dir.path().join("open.sock"),
        vec![file.display().to_string(), child.display().to_string()],
    )
    .await;
    fs::set_permissions(&child, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(result["error"]["class"], "rejected");
    assert_eq!(result["error"]["path_index"], 1);
    assert_eq!(doc!(app.editor).path(), Some(file.as_path()));
    assert!(app.close().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_open_closes_search_prompt_before_scratch_document_is_removed() {
    #[cfg(feature = "steel")]
    let _application = APPLICATION_LOCK.lock().await;
    let dir = private_dir();
    let socket = dir.path().join("editor.sock");
    let file = dir.path().join("file.txt");
    fs::write(&file, "text\n").unwrap();
    let mut app = application(socket.clone());
    let scratch = doc!(app.editor).id();
    keys(&mut app, "/").await;
    assert!(app.compositor.find::<ui::Prompt>().is_some());
    assert_eq!(
        request(&mut app, &socket, vec![file.display().to_string()]).await["status"],
        "ok"
    );
    assert!(app.editor.document(scratch).is_none());
    assert!(app.compositor.find::<ui::Prompt>().is_none());
    keys(&mut app, "<esc>").await;
    assert_eq!(doc!(app.editor).text().to_string(), "text\n");
    assert!(app.close().await.is_empty());
}

async fn check_pending_command(pending: &str) {
    #[cfg(feature = "steel")]
    let _application = APPLICATION_LOCK.lock().await;
    let dir = private_dir();
    let socket = dir.path().join("editor.sock");
    let first = dir.path().join("a.txt");
    let second = dir.path().join("b.txt");
    fs::write(&first, "(text)\n").unwrap();
    fs::write(&second, "x\n").unwrap();
    let mut app = application(socket.clone());
    assert_eq!(
        request(&mut app, &socket, vec![first.display().to_string()]).await["status"],
        "ok"
    );
    let first_id = doc!(app.editor).id();
    keys(&mut app, pending).await;
    assert_eq!(
        request(&mut app, &socket, vec![second.display().to_string()]).await["status"],
        "ok"
    );
    keys(&mut app, "?").await;
    assert_eq!(
        doc!(app.editor).text().to_string(),
        "x\n",
        "pending {pending} edited the new buffer"
    );
    assert_eq!(
        app.editor.document(first_id).unwrap().text().to_string(),
        "(text)\n"
    );
    keys(&mut app, "<esc>").await;
    assert!(app.close().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_open_cancels_pending_replace() {
    check_pending_command("r").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_open_cancels_pending_surround_replace() {
    check_pending_command("mr(").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_real_remote_callback_cannot_open_after_queue_is_drained() {
    #[cfg(feature = "steel")]
    let _application = APPLICATION_LOCK.lock().await;
    let dir = private_dir();
    let socket = dir.path().join("editor.sock");
    let file = dir.path().join("late.txt");
    fs::write(&file, "late\n").unwrap();
    let mut app = application(socket.clone());
    let scratch = doc!(app.editor).id();
    let requested_file = file.clone();
    let response = tokio::task::spawn_blocking(move || {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        writeln!(stream, "{}", json!({"version":1,"id":"expired","action":"open","paths":[requested_file],"timeout_ms":20})).unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str::<serde_json::Value>(&line).unwrap()
    }).await.unwrap();
    assert_eq!(response["error"]["class"], "timeout");
    let mut handled = 0;
    while let Ok(callback) = app.jobs.callbacks.try_recv() {
        app.jobs
            .handle_callback(&mut app.editor, &mut app.compositor, Ok(Some(callback)));
        handled += 1;
    }
    assert!(handled > 0);
    assert_eq!(doc!(app.editor).id(), scratch);
    assert!(app.editor.document_id_by_path(&file).is_none());
    assert!(app.close().await.is_empty());
}
