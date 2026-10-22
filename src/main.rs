mod app;
mod aws;
mod config;
mod tree;
mod ui;

use anyhow::Result;
use app::{App, Focus, Screen};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::time::Duration;

fn main() -> Result<()> {
    let args = parse_args();
    if args.dump_config {
        print!("{}", config::DEFAULT_CONFIG);
        return Ok(());
    }
    let cfg = config::Config::load(args.config.as_deref())?;
    #[cfg(feature = "demo")]
    let mut app = if args.demo { App::demo(cfg) } else { App::new(cfg, args.profile, args.region) };
    #[cfg(not(feature = "demo"))]
    let mut app = App::new(cfg, args.profile, args.region);

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);
    let _ = terminal.show_cursor();
    ratatui::restore();

    if let Some(out) = app.output_on_exit {
        println!("{out}");
    }
    result
}

#[derive(Default)]
struct Args {
    profile: Option<String>,
    region: Option<String>,
    config: Option<std::path::PathBuf>,
    dump_config: bool,
    #[cfg(feature = "demo")]
    demo: bool,
}

fn parse_args() -> Args {
    let mut out = Args::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--profile" | "-p" => out.profile = args.next(),
            "--region" | "-r" => out.region = args.next(),
            "--config" | "-c" => out.config = args.next().map(Into::into),
            "--dump-config" => out.dump_config = true,
            #[cfg(feature = "demo")]
            "--demo" => out.demo = true,
            "-h" | "--help" => {
                let path = config::Config::default_path().map(|p| p.display().to_string()).unwrap_or_default();
                println!(
                    "ecsterm [--profile NAME] [--region REGION] [--config PATH] [--dump-config]\n\n\
                     Browse ECS clusters > services > containers > tasks and run the matching AWS CLI commands.\n\n\
                     Options:\n  \
                     -p, --profile NAME   AWS profile to open (skips the picker)\n  \
                     -r, --region REGION  Override the profile's region\n  \
                     -c, --config PATH    Config file (default: {path})\n      \
                     --dump-config    Print the built-in default config as a starting point"
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other} (try --help)");
                std::process::exit(2);
            }
        }
    }
    out
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        if app.pending_run.is_some() {
            // Hand the terminal back in a sane state: cursor visible, cooked
            // mode, main screen. `restore()` alone leaves the cursor hidden.
            terminal.show_cursor()?;
            ratatui::restore();
            crossterm::execute!(std::io::stdout(), crossterm::cursor::Show)?;
            // Loops so a login accepted after a failed command runs before
            // returning to the TUI.
            while let Some(cmd) = app.pending_run.take() {
                let status = run_in_terminal(&cmd);
                // A successful login returns straight to the TUI; anything else
                // waits so its output can be read. A command that failed on a
                // profile with a login offers it, as an expired session fails
                // every other call the same way.
                let mut relogin = false;
                if let Ok(s) = &status {
                    let exit = format!("[exit {}]", s.code().unwrap_or(-1));
                    if !s.success() && !app.pending_login && app.can_login() {
                        relogin = offer_login(app, &exit);
                    } else if !s.success() || !app.pending_login {
                        println!();
                        pause(&format!("{exit} Press Enter to return to ecsterm…"));
                    }
                }
                app.status = match &status {
                    Ok(s) if s.success() => "Command finished.".into(),
                    Ok(s) => format!("Command exited with {}", s.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into())),
                    Err(e) => format!("Failed to run command: {e}"),
                };
                app.command_finished(matches!(status, Ok(s) if s.success()));
                if relogin {
                    app.login();
                }
            }
            *terminal = ratatui::init();
            terminal.clear()?;
            continue;
        }
        terminal.draw(|f| ui::draw(f, app))?;
        app.poll();
        if app.should_quit {
            return Ok(());
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            app.should_quit = true;
            continue;
        }
        match app.screen {
            Screen::Profiles => handle_profiles(app, key.code),
            Screen::Explorer => handle_explorer(app, key.code),
        }
    }
}

/// Runs `cmd` through the user's shell with the real terminal attached.
///
/// The child gets freshly opened `/dev/tty` descriptors instead of inheriting
/// our stdin/stdout. Interactive tools such as session-manager-plugin read the
/// tty directly and fail with "read /dev/stdin: input/output error" when the
/// inherited descriptor has been used by the TUI's event reader.
fn run_in_terminal(cmd: &str) -> Result<std::process::ExitStatus> {
    use std::fs::OpenOptions;
    use std::process::{Command, Stdio};

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    println!("\x1b[1m$ {cmd}\x1b[0m");
    let mut child = Command::new(shell);
    child.arg("-c").arg(cmd);
    if let (Ok(i), Ok(o), Ok(e)) = (
        OpenOptions::new().read(true).write(true).open("/dev/tty"),
        OpenOptions::new().read(true).write(true).open("/dev/tty"),
        OpenOptions::new().read(true).write(true).open("/dev/tty"),
    ) {
        child.stdin(Stdio::from(i)).stdout(Stdio::from(o)).stderr(Stdio::from(e));
    }
    Ok(child.status()?)
}

/// After a command failed on a profile with a login: checks whether its
/// session expired and if so asks to log in, otherwise pauses as usual.
/// Returns whether the user wants to log in.
fn offer_login(app: &App, exit: &str) -> bool {
    use std::io::Write;

    let profile = app.ecs.as_ref().map(|e| e.profile.as_str()).unwrap_or_default();
    // The check takes a moment; say why nothing is happening.
    print!("\n\x1b[2m{exit} Checking the session for {profile}…\x1b[0m");
    let _ = std::io::stdout().flush();
    if !app.session_expired() {
        pause(&format!("{exit} Press Enter to return to ecsterm…"));
        return false;
    }
    let answer = pause(&format!("{exit} The session for {profile} has expired. Press Enter to log in, or n to return to ecsterm…"));
    // Only a bare Enter or a yes logs in; anything else, EOF or a read error
    // returns to the TUI.
    matches!(answer.map(|a| a.trim().to_ascii_lowercase()).as_deref(), Some("" | "y" | "yes"))
}

/// Shows `prompt` on the current line, replacing what is there, and returns
/// the line typed before Enter, or `None` on EOF or a read error.
fn pause(prompt: &str) -> Option<String> {
    use std::io::{BufRead, Write};

    print!("\r\x1b[2K\x1b[2m{prompt}\x1b[0m ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

fn handle_profiles(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Up | KeyCode::Char('k') => app.profile_selected = app.profile_selected.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            if app.profile_selected + 1 < app.profiles.len() {
                app.profile_selected += 1;
            }
        }
        KeyCode::Enter => app.open_profile(None),
        _ => {}
    }
}

fn handle_explorer(app: &mut App, code: KeyCode) {
    // Modal: region input
    if let Some(input) = &mut app.region_input {
        match code {
            KeyCode::Esc => app.region_input = None,
            KeyCode::Enter => {
                let r = app.region_input.take().unwrap_or_default();
                if !r.trim().is_empty() {
                    app.set_region(r.trim().to_string());
                }
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => input.push(c),
            _ => {}
        }
        return;
    }
    // Modal: command editor
    if let Some(input) = &mut app.command_input {
        match code {
            KeyCode::Esc => app.command_input = None,
            KeyCode::Enter => {
                let c = app.command_input.take().unwrap_or_default();
                if !c.trim().is_empty() {
                    app.pending_run = Some(c);
                }
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => input.push(c),
            _ => {}
        }
        return;
    }
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('p') => app.screen = Screen::Profiles,
        KeyCode::Char('L') => app.login(),
        KeyCode::Char('r') => app.region_input = Some(app.ecs.as_ref().map(|e| e.region.clone()).unwrap_or_default()),
        KeyCode::Char('R') | KeyCode::F(5) => app.reload_selected(),
        // h/l and arrows switch between the tree and the command panel.
        KeyCode::Right | KeyCode::Char('l') => {
            if !app.commands.is_empty() {
                app.focus = Focus::Commands;
            }
        }
        KeyCode::Left | KeyCode::Char('h') => app.focus = Focus::Tree,
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        KeyCode::PageUp => app.move_selection(-10),
        KeyCode::PageDown => app.move_selection(10),
        KeyCode::Char('y') => app.copy_selected_command(),
        KeyCode::Char('e') => app.edit_command(),
        KeyCode::Esc => app.focus = Focus::Tree,
        _ => match app.focus {
            Focus::Tree => match code {
                KeyCode::Tab | KeyCode::Char(' ') => app.toggle_expand(),
                KeyCode::Enter => app.expand_or_activate(),
                _ => {}
            },
            Focus::Commands => match code {
                KeyCode::Enter => app.activate_command(),
                _ => {}
            },
        },
    }
}
