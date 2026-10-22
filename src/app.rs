use crate::aws::{self, Ecs, Profile};
use crate::config::{self, Action, Config, When};
use std::collections::HashMap;
use crate::tree::{Kind, Node, Row, Tree};
use anyhow::Result;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

/// Results coming back from background `aws` calls, tagged with the
/// `logins` count from when the call started.
pub enum Msg {
    Children(Vec<usize>, u32, Result<Vec<Node>>),
    /// Credential check for the named profile.
    Auth(String, u32, Result<()>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Focus {
    Tree,
    Commands,
}

#[derive(Debug, Clone)]
pub struct CliCommand {
    pub title: String,
    pub command: String,
}

pub enum Screen {
    Profiles,
    Explorer,
}

pub struct App {
    pub config: Config,
    pub screen: Screen,
    pub profiles: Vec<Profile>,
    pub profile_selected: usize,

    pub ecs: Option<Ecs>,
    pub tree: Tree,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub focus: Focus,

    pub commands: Vec<CliCommand>,
    pub command_selected: usize,
    /// Tree path the commands were built for; while it stays selected,
    /// rebuilding keeps `command_selected`.
    commands_path: Option<Vec<usize>>,

    pub region_input: Option<String>,
    /// Editable command text shown in a modal before running.
    pub command_input: Option<String>,
    /// Command to execute in the real terminal; consumed by the main loop.
    pub pending_run: Option<String>,
    /// The pending/running command is a login; load the tree once it succeeds.
    pub pending_login: bool,
    pub status: String,
    pub should_quit: bool,
    /// Printed to stdout after the terminal is restored.
    pub output_on_exit: Option<String>,

    /// Successful logins so far. Background results from before the latest
    /// one carry the old credentials' failures.
    logins: u32,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl App {
    const HELP: &str = "↑↓/jk move  Tab expand/collapse  ←→/hl switch panel  Enter run  y copy  e edit  r region  L login  p profile  q quit";

    pub fn new(config: Config, profile: Option<String>, region: Option<String>) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut profiles = aws::load_profiles();
        if let Some(p) = &profile {
            if !profiles.iter().any(|x| &x.name == p) {
                profiles.insert(0, Profile { name: p.clone(), region: None, login: None });
            }
        }
        let mut app = App {
            config,
            screen: Screen::Profiles,
            profiles,
            profile_selected: 0,
            ecs: None,
            tree: Tree::new(String::new()),
            rows: vec![],
            selected: 0,
            focus: Focus::Tree,
            commands: vec![],
            command_selected: 0,
            commands_path: None,
            region_input: None,
            command_input: None,
            pending_run: None,
            pending_login: false,
            status: String::new(),
            should_quit: false,
            output_on_exit: None,
            logins: 0,
            tx,
            rx,
        };
        if let Some(p) = profile {
            let idx = app.profiles.iter().position(|x| x.name == p).unwrap_or(0);
            app.profile_selected = idx;
            app.open_profile(region);
        }
        app
    }

    /// Fake, pre-expanded tree for screenshots. Makes no AWS calls.
    #[cfg(feature = "demo")]
    pub fn demo(config: Config) -> Self {
        use crate::aws::{Cluster, Service, Task};
        let mut app = Self::new(config, None, None);
        let region = "eu-west-1".to_string();
        app.ecs = Some(Ecs { profile: "acme-prod".into(), region: region.clone() });
        app.tree = Tree::new(region);
        app.screen = Screen::Explorer;
        app.status = Self::HELP.into();

        let cluster = |name: &str| Cluster {
            arn: format!("arn:aws:ecs:eu-west-1:123456789012:cluster/{name}"),
            name: name.into(),
        };
        let service = |c: &Cluster, name: &str, rev: u32, exec: bool| Service {
            arn: format!("arn:aws:ecs:eu-west-1:123456789012:service/{}/{name}", c.name),
            name: name.into(),
            status: "ACTIVE".into(),
            cluster_arn: c.arn.clone(),
            task_definition: format!("arn:aws:ecs:eu-west-1:123456789012:task-definition/{name}:{rev}"),
            enable_execute_command: exec,
        };
        let task = |c: &Cluster, id: &str, status: &str, containers: &[(&str, bool)]| Task {
            arn: format!("arn:aws:ecs:eu-west-1:123456789012:task/{}/{id}", c.name),
            last_status: status.into(),
            desired_status: "RUNNING".into(),
            containers: containers.iter().map(|(n, a)| (n.to_string(), *a)).collect(),
        };
        let container = |c: &Cluster, s: &Service, name: &str, tasks: Vec<Task>| {
            Node::new(Kind::Container { name: name.into(), cluster: c.clone(), service: s.clone(), tasks })
        };
        let node = |kind: Kind, expanded: bool, children: Vec<Node>| {
            let mut n = Node::new(kind);
            n.expanded = expanded;
            n.children = Some(children);
            n
        };

        let api = cluster("acme-prod-api");
        let api_web = service(&api, "acme-prod-api", 42, true);
        let api_worker = service(&api, "acme-prod-api-worker", 42, true);
        let web_tasks = vec![
            task(&api, "3f9c2a7e1b4d4c0e9a8b7c6d5e4f3a2b", "RUNNING", &[("api", true), ("nginx", true)]),
            task(&api, "a1b2c3d4e5f60718293a4b5c6d7e8f90", "RUNNING", &[("api", true), ("nginx", true)]),
            task(&api, "0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f", "PENDING", &[("api", false), ("nginx", false)]),
        ];
        let mut api_container = container(&api, &api_web, "api", web_tasks.clone());
        api_container.expanded = true;
        let api_web_node = node(
            Kind::Service(api_web.clone()),
            true,
            vec![api_container, container(&api, &api_web, "nginx", web_tasks)],
        );
        let api_worker_node = node(Kind::Service(api_worker), false, vec![]);
        let api_node = node(Kind::Cluster(api.clone()), true, vec![api_web_node, api_worker_node]);

        let billing = cluster("acme-prod-billing");
        let billing_svc = service(&billing, "acme-prod-billing", 17, false);
        let billing_node = node(
            Kind::Cluster(billing.clone()),
            true,
            vec![node(Kind::Service(billing_svc), false, vec![])],
        );
        let auth_node = node(Kind::Cluster(cluster("acme-prod-auth")), false, vec![]);
        let staging_node = node(Kind::Cluster(cluster("acme-staging")), false, vec![]);

        let ecs = app.tree.get_mut(&[0]).expect("ECS node");
        ecs.children = Some(vec![api_node, auth_node, billing_node, staging_node]);
        app.refresh_rows();
        // Land on the first running task so the command panel shows the exec commands.
        app.selected = app
            .rows
            .iter()
            .position(|r| r.message.is_none() && app.tree.get(&r.path).map(|n| n.is_leaf()).unwrap_or(false))
            .unwrap_or(0);
        app.rebuild_commands();
        app
    }

    fn fallback_region() -> String {
        std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".into())
    }

    pub fn open_profile(&mut self, region_override: Option<String>) {
        let Some(p) = self.profiles.get(self.profile_selected).cloned() else { return };
        let region = region_override.or(p.region).unwrap_or_else(Self::fallback_region);
        self.ecs = Some(Ecs { profile: p.name, region: region.clone() });
        self.tree = Tree::new(region);
        self.selected = 1; // the ECS row
        self.commands.clear();
        self.screen = Screen::Explorer;
        self.refresh_rows();
        self.check_credentials();
    }

    /// Verifies the profile's credentials in the background before loading,
    /// so an expired session can trigger its login command first.
    fn check_credentials(&mut self) {
        let Some(ecs) = self.ecs.clone() else { return };
        self.status = format!("Checking credentials for {}…", ecs.profile);
        let (tx, logins) = (self.tx.clone(), self.logins);
        thread::spawn(move || {
            let res = ecs.check_credentials();
            let _ = tx.send(Msg::Auth(ecs.profile, logins, res));
        });
    }

    fn current_login(&self) -> Option<aws::Login> {
        let ecs = self.ecs.as_ref()?;
        self.profiles.iter().find(|p| p.name == ecs.profile)?.login
    }

    pub fn can_login(&self) -> bool {
        self.current_login().is_some()
    }

    /// Runs the profile's login command (`aws sso login` / `aws login`).
    pub fn login(&mut self) {
        let Some(ecs) = &self.ecs else { return };
        match self.current_login() {
            Some(l) => {
                self.pending_run = Some(l.command(&ecs.profile));
                self.pending_login = true;
            }
            None => self.status = format!("Profile {} has no sso_session or login_session to log in with", ecs.profile),
        }
    }

    /// Whether `aws` says the current profile's session has expired, as
    /// opposed to working or being unreachable. Blocks on an `aws` call, so
    /// only use it after a command failed.
    pub fn session_expired(&self) -> bool {
        let Some(ecs) = &self.ecs else { return false };
        match ecs.check_credentials() {
            Ok(()) => false,
            Err(e) => aws::is_expired_session(&format!("{e:#}")),
        }
    }

    /// Called by the main loop after `pending_run` has finished.
    pub fn command_finished(&mut self, success: bool) {
        if std::mem::take(&mut self.pending_login) {
            if success {
                self.logins += 1;
                self.status = Self::HELP.into();
                self.reload_failed();
            } else {
                self.status = "Login failed. Press L to try again.".into();
            }
        }
    }

    /// After a login: retries every node that failed, including the clusters
    /// when the credential check stopped them from loading. The rest of the
    /// tree and the selection stay, so a login mid-session returns to where the
    /// user was. Loads still running retry when they fail (see `poll`).
    fn reload_failed(&mut self) {
        for path in self.tree.failed() {
            self.load_children(path);
        }
        self.refresh_rows();
    }

    fn reload_root(&mut self) {
        if let Some(n) = self.tree.get_mut(&[0]) {
            n.children = None;
        }
        self.load_children(vec![0]);
        self.refresh_rows();
    }

    pub fn set_region(&mut self, region: String) {
        if let Some(ecs) = &mut self.ecs {
            ecs.region = region.clone();
        }
        self.tree = Tree::new(region);
        self.selected = 1;
        self.commands.clear();
        self.load_children(vec![0]);
        self.refresh_rows();
    }

    pub fn refresh_rows(&mut self) {
        // Follow the selected node when rows above it appear or disappear,
        // e.g. a failed node reloading, so Enter still runs what was selected.
        // `rebuild_commands` keeps its highlighted command the same way.
        let selected = self.rows.get(self.selected).filter(|r| r.message.is_none()).map(|r| r.path.clone());
        self.rows = self.tree.rows();
        if let Some(i) = selected.and_then(|p| self.rows.iter().position(|r| r.message.is_none() && r.path == p)) {
            self.selected = i;
        }
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
        self.rebuild_commands();
    }

    pub fn selected_node(&self) -> Option<(&Row, &Node)> {
        let row = self.rows.get(self.selected)?;
        if row.message.is_some() {
            return None;
        }
        Some((row, self.tree.get(&row.path)?))
    }

    // ----- background loading -------------------------------------------------

    fn load_children(&mut self, path: Vec<usize>) {
        let Some(ecs) = self.ecs.clone() else { return };
        let Some(node) = self.tree.get_mut(&path) else { return };
        if node.loading {
            return;
        }
        node.loading = true;
        node.error = None;
        let kind = node.kind.clone();
        let (tx, logins) = (self.tx.clone(), self.logins);
        thread::spawn(move || {
            let res = load(&ecs, &kind);
            let _ = tx.send(Msg::Children(path, logins, res));
        });
    }

    /// Drain background results. Returns true if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            changed = true;
            match msg {
                Msg::Children(path, logins, res) => {
                    let Some(node) = self.tree.get_mut(&path) else { continue };
                    node.loading = false;
                    match res {
                        Ok(children) => node.children = Some(children),
                        // Started before a login; the new credentials may work.
                        Err(_) if logins != self.logins => self.load_children(path),
                        Err(e) => {
                            let e = format!("{e:#}");
                            let expired = aws::is_expired_session(&e);
                            node.error = Some(e);
                            if expired && self.can_login() {
                                let profile = self.ecs.as_ref().map(|e| e.profile.as_str()).unwrap_or_default();
                                self.status = format!("The session for {profile} has expired. Press L to log in.");
                            }
                        }
                    }
                    self.refresh_rows();
                }
                Msg::Auth(profile, logins, res) => {
                    // Ignore results for a profile the user has since left.
                    if self.ecs.as_ref().map(|e| e.profile != profile).unwrap_or(true) {
                        continue;
                    }
                    // A login finished while this ran; check the new credentials.
                    if logins != self.logins {
                        self.check_credentials();
                        continue;
                    }
                    match res {
                        Ok(()) => {
                            self.status = Self::HELP.into();
                            self.reload_root();
                        }
                        Err(e) if self.can_login() => {
                            // Shown if the login fails; `reload_failed` retries it.
                            if let Some(n) = self.tree.get_mut(&[0]) {
                                n.error = Some(format!("{e:#}"));
                            }
                            self.refresh_rows();
                            self.login();
                        }
                        Err(e) => {
                            self.status = format!("Credentials for {profile} failed: {e:#}");
                            self.reload_root();
                        }
                    }
                }
            }
        }
        changed
    }

    // ----- navigation -----------------------------------------------------------

    pub fn move_selection(&mut self, delta: i32) {
        match self.focus {
            Focus::Tree => {
                let len = self.rows.len() as i32;
                if len == 0 {
                    return;
                }
                self.selected = (self.selected as i32 + delta).clamp(0, len - 1) as usize;
                self.rebuild_commands();
            }
            Focus::Commands => {
                let len = self.commands.len() as i32;
                if len == 0 {
                    return;
                }
                self.command_selected = (self.command_selected as i32 + delta).clamp(0, len - 1) as usize;
            }
        }
    }

    pub fn expand_or_activate(&mut self) {
        let Some((row, node)) = self.selected_node() else { return };
        let path = row.path.clone();
        if node.is_leaf() || node.expanded {
            // Enter on an open node focuses its commands.
            if !self.commands.is_empty() {
                self.focus = Focus::Commands;
            }
            return;
        }
        let needs_load = node.children.is_none() || node.error.is_some();
        if let Some(n) = self.tree.get_mut(&path) {
            n.expanded = true;
        }
        if needs_load {
            self.load_children(path);
        }
        self.refresh_rows();
    }

    /// Tab: open a closed node, close an open one.
    pub fn toggle_expand(&mut self) {
        let Some((_, node)) = self.selected_node() else { return };
        if node.expanded && !node.is_leaf() {
            self.collapse();
        } else {
            self.expand_or_activate();
        }
    }

    pub fn collapse(&mut self) {
        let Some(row) = self.rows.get(self.selected) else { return };
        let path = row.path.clone();
        let is_msg = row.message.is_some();
        let expanded = self.tree.get(&path).map(|n| n.expanded && !n.is_leaf()).unwrap_or(false);
        if expanded && !is_msg {
            if let Some(n) = self.tree.get_mut(&path) {
                n.expanded = false;
            }
        } else if let Some(parent) = path.split_last().map(|(_, p)| p.to_vec()) {
            // Jump to parent row.
            if let Some(i) = self.rows.iter().position(|r| r.message.is_none() && r.path == parent) {
                self.selected = i;
            }
        }
        self.refresh_rows();
    }

    pub fn reload_selected(&mut self) {
        let Some((row, node)) = self.selected_node() else { return };
        if node.is_leaf() {
            return;
        }
        let path = row.path.clone();
        if let Some(n) = self.tree.get_mut(&path) {
            n.expanded = true;
            n.children = None;
        }
        self.load_children(path);
        self.refresh_rows();
    }

    /// Enter on a command: fill in missing pieces first, then run.
    pub fn activate_command(&mut self) {
        let Some(c) = self.selected_command().cloned() else { return };
        if c.command.contains('<') && c.command.contains('>') {
            self.command_input = Some(c.command);
            return;
        }
        self.pending_run = Some(c.command);
    }

    pub fn edit_command(&mut self) {
        if let Some(c) = self.selected_command().cloned() {
            self.command_input = Some(c.command);
        }
    }

    // ----- commands -------------------------------------------------------------

    pub fn rebuild_commands(&mut self) {
        let path = self.selected_node().map(|(row, _)| row.path.clone());
        let keep = if path.is_some() && path == self.commands_path { self.command_selected } else { 0 };
        self.commands_path = path;
        self.commands.clear();
        self.command_selected = 0;
        let Some(ecs) = self.ecs.clone() else { return };
        let Some((_, node)) = self.selected_node() else { return };

        let mut vars: HashMap<&str, String> = HashMap::from([
            ("aws", ecs.prefix()),
            ("profile", ecs.profile.clone()),
            ("region", ecs.region.clone()),
        ]);
        let mut exec_disabled = false;
        let put_cluster = |vars: &mut HashMap<&str, String>, c: &aws::Cluster| {
            vars.insert("cluster", c.name.clone());
            vars.insert("cluster_arn", c.arn.clone());
        };
        let put_service = |vars: &mut HashMap<&str, String>, s: &aws::Service| {
            vars.insert("service", s.name.clone());
            vars.insert("service_arn", s.arn.clone());
            vars.insert("task_definition", s.task_definition.clone());
        };

        let actions: &[Action] = match &node.kind {
            Kind::Region { .. } | Kind::Ecs => self.config.ecs.as_deref().unwrap_or_default(),
            Kind::Cluster(c) => {
                put_cluster(&mut vars, c);
                self.config.cluster.as_deref().unwrap_or_default()
            }
            Kind::Service(s) => {
                put_cluster(&mut vars, &aws::Cluster { arn: s.cluster_arn.clone(), name: cluster_name(&s.cluster_arn) });
                put_service(&mut vars, s);
                exec_disabled = !s.enable_execute_command;
                self.config.service.as_deref().unwrap_or_default()
            }
            Kind::Container { name, cluster, service, .. } => {
                put_cluster(&mut vars, cluster);
                put_service(&mut vars, service);
                vars.insert("container", name.clone());
                exec_disabled = !service.enable_execute_command;
                self.config.container.as_deref().unwrap_or_default()
            }
            Kind::Task { task, container, cluster, service } => {
                put_cluster(&mut vars, cluster);
                put_service(&mut vars, service);
                vars.insert("container", container.clone());
                vars.insert("task", task.id().to_string());
                vars.insert("task_arn", task.arn.clone());
                exec_disabled = !service.enable_execute_command;
                self.config.task.as_deref().unwrap_or_default()
            }
        };

        self.commands = actions
            .iter()
            .filter(|a| match a.when {
                Some(When::ExecDisabled) => exec_disabled,
                None => true,
            })
            .map(|a| CliCommand { title: a.title.clone(), command: config::render(&a.command, &vars) })
            .collect();
        self.command_selected = keep.min(self.commands.len().saturating_sub(1));
    }

    pub fn selected_command(&self) -> Option<&CliCommand> {
        self.commands.get(self.command_selected)
    }

    pub fn copy_selected_command(&mut self) {
        let Some(c) = self.selected_command().cloned() else { return };
        match copy_to_clipboard(&c.command) {
            Ok(()) => self.status = format!("Copied: {}", c.title),
            Err(e) => self.status = format!("Copy failed: {e}"),
        }
    }
}

fn cluster_name(arn: &str) -> String {
    arn.rsplit('/').next().unwrap_or(arn).to_string()
}

fn load(ecs: &Ecs, kind: &Kind) -> Result<Vec<Node>> {
    Ok(match kind {
        Kind::Ecs => ecs.list_clusters()?.into_iter().map(|c| Node::new(Kind::Cluster(c))).collect(),
        Kind::Cluster(c) => ecs.list_services(&c.arn)?.into_iter().map(|s| Node::new(Kind::Service(s))).collect(),
        Kind::Service(s) => {
            // Two independent CLI round-trips; run them concurrently.
            let (td, tasks) = thread::scope(|sc| {
                let td = sc.spawn(|| ecs.describe_task_definition(&s.task_definition));
                let tasks = sc.spawn(|| ecs.list_tasks(&s.cluster_arn, &s.name));
                (td.join().expect("thread panicked"), tasks.join().expect("thread panicked"))
            });
            let (td, tasks) = (td?, tasks?);
            let cluster = aws::Cluster { arn: s.cluster_arn.clone(), name: cluster_name(&s.cluster_arn) };
            td.containers
                .into_iter()
                .map(|name| {
                    Node::new(Kind::Container {
                        name,
                        cluster: cluster.clone(),
                        service: s.clone(),
                        tasks: tasks.clone(),
                    })
                })
                .collect()
        }
        Kind::Region { .. } | Kind::Container { .. } | Kind::Task { .. } => vec![],
    })
}

fn copy_to_clipboard(text: &str) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let candidates: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["pbcopy"]]
    } else {
        &[&["wl-copy"], &["xclip", "-selection", "clipboard"], &["xsel", "--clipboard", "--input"]]
    };
    let mut last = None;
    for cand in candidates {
        match Command::new(cand[0]).args(&cand[1..]).stdin(Stdio::piped()).spawn() {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(text.as_bytes())?;
                }
                child.wait()?;
                return Ok(());
            }
            Err(e) => last = Some(e),
        }
    }
    Err(anyhow::anyhow!("no clipboard tool found: {:?}", last))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::tests::cluster;

    /// ECS > [failed cluster (load error), open cluster > target cluster].
    fn app_with_tree() -> App {
        let mut app = App::new(Config::defaults(), None, None);
        app.ecs = Some(Ecs { profile: "test".into(), region: "eu-west-1".into() });
        let mut failed = cluster("failed");
        failed.expanded = true;
        failed.error = Some("Token has expired".into());
        let mut open = cluster("open");
        open.expanded = true;
        open.children = Some(vec![cluster("target")]);
        app.tree.get_mut(&[0]).unwrap().children = Some(vec![failed, open]);
        app.refresh_rows();
        app.selected = app.rows.iter().position(|r| r.path == [0, 1, 0]).unwrap();
        app.rebuild_commands();
        app
    }

    fn reload_failed_node(app: &mut App) {
        let n = app.tree.get_mut(&[0, 0]).unwrap();
        n.error = None;
        n.children = Some(vec![cluster("a"), cluster("b")]);
        app.refresh_rows();
    }

    #[test]
    fn selection_follows_its_node_when_rows_above_change() {
        let mut app = app_with_tree();
        reload_failed_node(&mut app);
        assert_eq!(app.rows[app.selected].path, [0, 1, 0]);
        assert_eq!(app.selected_node().unwrap().1.label(), "target");
    }

    #[test]
    fn highlighted_command_survives_a_refresh_but_not_moving() {
        let mut app = app_with_tree();
        assert!(app.commands.len() > 1, "cluster level needs two commands for this test");
        app.focus = Focus::Commands;
        app.command_selected = 1;
        reload_failed_node(&mut app);
        assert_eq!(app.command_selected, 1);

        app.focus = Focus::Tree;
        app.move_selection(-1);
        assert_eq!(app.command_selected, 0);
    }
}
